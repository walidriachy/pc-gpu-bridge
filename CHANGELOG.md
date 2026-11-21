# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.4] — 2026-11-21

### Changed
- **The router now really learns from both machines.** Every time a heavy job runs, PC Bridge writes down how long it took, on the Mac or on the PC, and uses that to choose next time. Before, it only remembered PC jobs, and it counted the whole trip there and back (sending files, waiting in line, bringing results back) as if it were the PC's work, so the PC looked slower than it is. Now it keeps the PC's real working time apart from the waiting and the file transfers, and adds those once.
- It tells jobs apart: `python train.py` and `python tiny.py` are learned separately, as are different ffmpeg codecs, Blender scripts, npm scripts and Ollama models. The Mac's and the PC's numbers are only compared when they come from the same kind of job, and recent runs count more than old ones.
- Small jobs (inputs under 1 MB) were estimated up to 1000 times too long once they had been learned. They are now measured the same way they are estimated.
- Results the PC already had (from its result cache) and failed jobs aren't learned from. Your existing history is kept and upgraded on first start; old PC timings, which included the round trip, are no longer used for learning.
- **ComfyUI images wait their turn for the graphics card.** A ComfyUI image job now takes the same single GPU slot as a render or a PyTorch job, so two of them no longer run on the graphics card at the same time and run out of memory. While it waits, it shows in the queue like any other job and can be cancelled. If ComfyUI is busy for a moment and doesn't answer, PC Bridge keeps waiting instead of handing the card to the next job; if it stops answering for 2 minutes, PC Bridge stops our image and only then frees the card.
- The PC's job summary now says how long a job waited in line (`queue_secs`) and whether it came from the cache (`cached`). `duration` is now only the time the program itself ran, not the time spent collecting its results.

### Fixed
- **Safer file sync.** Files sent to the PC are written under a temporary name and swapped in only when complete, so a job or the sync never sees half a file. When a synced file is overwritten or deleted, Syncthing now keeps the old copy for 14 days in a hidden `.stversions` folder inside the shared folder, instead of it being gone. `.git` and `.claude` folders are no longer synced: two computers writing one Git repository at once can corrupt it. Changes are sent after 2 seconds instead of 10. Folders shared by earlier versions pick up these settings the next time the Mac app starts.
- **Automatic updates never cut off a job.** The PC app already skipped updating while a job ran; it now also waits for jobs still in the queue, and if a job starts while the update downloads, it waits for that job to finish (up to 3 hours, then tries again later) before restarting.
- **The Jobs list survives a restart.** The PC now reloads the last 7 days of jobs when it starts, so the list no longer empties after a reboot or an update. A job that was still running when PC Bridge stopped shows as "Stopped (PC Bridge restarted)".
- **No more Mac password prompt after every update.** The pairing key used to live in the macOS Keychain. Because PC Bridge is signed for free (ad hoc), every update looked like a new app to macOS, which asked for your Mac password again, and until someone answered, nothing could reach the PC. The key now lives in `~/.pcbridge/token`, a file only your Mac user can read (the same way the PC keeps its copy). It is moved there automatically the first time 0.1.4 starts: macOS may ask one last time. If it does, type your password only into that macOS window, never into Claude or any chat.
- **A refused key is no longer reported as "PC asleep".** When the PC is on but doesn't accept this Mac's key, PC Bridge now says so right away ("Pair again: Settings → Run setup again") instead of sending Wake-on-LAN, waiting 90 seconds and saying the PC couldn't be reached. The menu bar and the status screen say the same.
- **Pairing the same Mac again no longer adds a duplicate.** Each Mac now has a permanent id. Pairing again replaces its old entry on the PC, and the old key stops working. The first time 0.1.4 starts, the Mac also removes the extra copies earlier versions left behind (for example "Walid's MacBook Pro" listed three times). **Unpair** on the Mac now tells the PC to forget this Mac too, and the PC app's **Settings → Pairing** lists the paired Macs with a **Remove** button.
- **Diagnostics name buttons that really exist.** Some "how to fix" tips pointed to buttons that don't exist ("Pair again", "Connect to Claude", "Setup → step 2"). Every tip on the Mac and the PC now names the real path, for example *Settings → "Run setup again" → "Pair with your PC"* or *Settings → "Reconnect to Claude"*.
- **Cancel really stops the job on the PC.** Many jobs start the real work as a second program (PowerShell starting ffmpeg, `npm` starting node, Claude starting its tools). Cancel used to stop only the first one, so the real work kept running on the graphics card and the job didn't end until it finished by itself. Now Cancel (and the install time limit) stops the whole job: on Windows with `taskkill /T`. Pressing **Ctrl-C** on a command that went to the PC now also cancels it on the PC, instead of leaving it running there.
- **Commands no longer get garbled on their way to the PC.** Numbers such as `1.5` or `29.97`, sentences in a prompt (`"a red car. 4k"`) and PowerShell code were mistaken for file names and replaced with a PC folder path, so the job failed and quietly ran on the Mac instead. Now a relative argument is only treated as a file when that file really exists. A Mac path inside a shared folder is translated even when it doesn't exist yet (a new output folder), and Mac paths inside PowerShell or `cmd` code are translated and put in quotes, so folder names with spaces (like "Created Apps in claude") keep working. `cmd /c …` is no longer refused by the PC as an "outside the folder" path.
- A ComfyUI image that failed inside ComfyUI (for example, the graphics card ran out of memory) is now reported as failed instead of "done" with no images.

## [0.1.3] — 2026-11-21

### Added
- **Missing programs: a clear message and a one-click install.** When a job needs a program the PC doesn't have, the job now says exactly what's missing, for example *"Blender isn't installed on the PC. Ask Claude to install it (tool pc_install, program "blender"), or on the PC open PC Bridge → Settings → Programs → Install."* The PC notices when the program can't be started (exit code 127), when a Python job reports `No module named 'torch'` (or `torchvision`, `torchaudio`), and when a script's shell says a known program "is not recognized" or "not found" (only real shell messages, not other errors that happen to name a program). A job that ran a different Python (a venv, conda, `py -3.11`) is told to install the package into that Python with pip instead, because `pc_install` only fills the PC's main Python. As before, a job the PC accepted is never run again on the Mac.
- **New Claude tool `pc_install`** (15 tools now). It installs one program on the PC: `ffmpeg`, `blender`, `pytorch`, `ollama`, `node`, `docker` or `git`. Claude is told to ask you first and to warn you that Windows may show a "Yes" prompt on the PC. `pc_run`, `pc_docker` and `pc_node_build` results end with a note when a program was missing, and `pc_status` adds a "Programs on the PC: installed … · not installed …" line (the PC's status now also reports `pytorch` and `git`, and finds Blender outside PATH).
- **The CLAUDE.md block has one new line:** if a job fails because a program is missing on the PC, offer to install it with `pc_install` (ask the user first). Click **Settings → Reconnect to Claude** on the Mac to add it to an existing setup.
- **PC: Settings → Programs.** Every program PC Bridge can install, what it's for, its size, and ✅ **Installed** or an **Install** button. It shows the free space on the Windows drive, warns under 10 GB, turns off **Install** when a program won't fit, and keeps the installer's last lines under **Show what the installer said**.
- **PC wizard, step 6: "PyTorch (GPU)"** (about 6 GB) next to Ollama and ComfyUI, with a free-space check on drive C:. The wizard's final GPU test needs it.
- **PC helper: `GET /programs` and `POST /programs/install`.** Only programs from a fixed catalog can be installed, with winget or pip, in their normal Windows place, never in the PCBridge or shared folders. The helper checks for the program's size plus 2 GB of free space first (otherwise it answers with how much is free and needed), and a second request for the same program follows the install already running, even when both arrive at the same moment. winget's "already installed" answer counts as success; "installed, restart the PC to finish" counts as success and says to restart (in `pc_install` and on the Programs card), and "restart before installing" says to restart and try again. pip installs PyTorch without keeping a second copy of the download in its cache.
- **Installs don't hold up jobs.** Installs have their own lane, one at a time, so a long PyTorch download or a Windows "Yes" prompt nobody is there to click never makes `pc_run` or `pc_delegate_task` wait, and an install doesn't wait for a long render. If a winget install shows no progress for 90 seconds, its log says Windows may be waiting for someone to click **Yes** on the PC. After 20 minutes without progress (or 3 hours in all) the install is stopped with a plain explanation.
- The job summary has new `missing` (the missing program's key, or `null`) and `restart_needed` fields.

### Changed
- **Faster jobs on big shared folders.** To find a job's results, the PC now looks only in the folder the job ran in (2 levels of sub-folders; all of it for worker-Claude jobs) and in folders named on the command line inside the PCBridge folder (up to 32; for Blender's `-o //renders/…`, the `.blend` file's folder), with a 5-second limit per check. Before, it listed the whole folder before and after every job, which took 20–45 s on very big shared folders. **Limitation:** files a job writes anywhere else in a shared folder still reach the Mac through Syncthing, but aren't listed as the job's outputs, copied into the job folder or reused by the result cache.
- **`python` jobs run the PC's own Python.** A bare `python` or `python3` now runs the Python that PyTorch is installed into. PC Bridge's private Python and the Microsoft Store placeholder (which only opens the Store) are skipped; a Python installed from the Microsoft Store is used.
- **Blender is found even when it isn't on PATH.** Blender's installer doesn't add itself to PATH, so the PC also looks in `C:\Program Files\Blender Foundation\` and uses the newest version.
- When the PC refuses a request because space is low or winget/Python is missing, the Mac shows the PC's own plain sentence instead of "HTTP 507 …" / "HTTP 503 …". This also applies to the existing Ollama and ComfyUI messages.
- "Program not found on the PC" now adds "(it isn't installed there, or Windows can't find it)".

### Fixed
- **Mac: "PC Bridge is damaged and can't be opened" (0.1.2).** The Mac app only had the compiler's temporary signature. It's now signed ad-hoc (free, no Apple certificate) before the `.dmg` and the update file are made, so macOS offers the normal **Open Anyway** instead. The release workflow checks the signature of the app, the update file and the `.dmg`, and stops if one is missing.
- **Mac: the app couldn't update itself after "Open Anyway".** macOS sometimes ran a hidden, read-only copy of the app (App Translocation). At start, PC Bridge now removes the "downloaded from the internet" mark from `/Applications/PC Bridge.app` and restarts from there once. If it isn't in Applications, it asks you to move it there. It only restarts from Applications when the copy there is the same version; a new version opened straight from the `.dmg` asks you to drag it into Applications instead of starting the old one. If Claude's connection or the helper commands still point at the hidden copy (connected while 0.1.2 ran from there), PC Bridge points them at the Applications copy again. 0.1.2 installs that run from the hidden copy can't update themselves: install 0.1.3 by hand once (see the guide).
- Text from a program's output is no longer garbled when a non-English character is split between two reads.

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

[Unreleased]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/walidriachy/pc-gpu-bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/walidriachy/pc-gpu-bridge/releases/tag/v0.1.0

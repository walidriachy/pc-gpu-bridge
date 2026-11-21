# PC Bridge — beginner guide

This guide takes you from nothing to "Claude uses my PC's graphics card" in about 30–45 minutes. Most of that is waiting for downloads. You won't need Terminal.

**The big idea:** your Mac is the *brain* and your PC is the *muscle*. You keep working with Claude on your Mac. When a job is heavy, such as encoding a video, generating images, running a big data script or building a large project, PC Bridge quietly sends it to your PC. The result appears back in the same Mac folder.

---

## What you need

* Your **Windows PC** (with the NVIDIA graphics card), switched on and connected to the internet. Wired Ethernet is best.
* Your **Mac**.
* About **10 GB free** on the PC.
* A free **Tailscale** account. You'll create it during setup with Google, Microsoft, Apple or GitHub sign-in, and you'll use the **same account on both computers**.
* Your **Claude** account, for the optional "worker Claude" on the PC.

**Order matters:** set up the **PC first**, then the **Mac**. At the end, the PC shows a 6-digit code that you type on the Mac.

---

## Part 1 — Download

1. Open the latest release: <https://github.com/walidriachy/pc-gpu-bridge/releases/latest>
2. On the **PC**, download **`PC Bridge Setup.exe`**.
3. On the **Mac**, download **`PC Bridge.dmg`**.

> Your browser may name the files `PC.Bridge.Setup.exe` and `PC.Bridge.dmg`, with dots instead of spaces. That's normal; they're the same files.

---

## Part 2 — Opening an app that isn't "signed"

PC Bridge isn't signed with a paid Apple or Microsoft certificate, so both systems warn you the first time. This is expected, and you only do it once.

### On Windows ("Windows protected your PC")

1. Double-click **PC Bridge Setup.exe**.
2. If a blue box says **"Windows protected your PC"**, click **More info**.
3. Click **Run anyway**.
4. The installer finishes by itself. It installs only for your user and doesn't need admin rights. PC Bridge opens when it's done.

### On the Mac ("can't be opened because Apple cannot check it")

1. Double-click **PC Bridge.dmg**, then drag **PC Bridge** onto the **Applications** folder.
2. Open **Applications**, **right-click** (or Control-click) **PC Bridge**, and choose **Open**.
3. Click **Open** again in the box that appears.

If you double-clicked and only saw a box with no Open button:

1. Open **System Settings → Privacy & Security**.
2. Scroll down. You'll see *"PC Bridge was blocked…"*. Click **Open Anyway**.
3. Confirm with your password or Touch ID.

---

## Part 3 — The PC wizard (9 steps)

Each step tells you **what** it's doing and **why**. Most steps have a **Do it for me** button and then check the result by themselves: ✅ means done, ❌ means it shows you what to do. You can close the app at any time; it reopens at the same step.

| Step | What happens | What you do |
|---|---|---|
| **1. Welcome** | Shows where PC Bridge will keep its files: `D:\PCBridge` (or `C:\PCBridge` if there's no D: drive) | Keep the suggested folder, or pick another. Click **Next** |
| **2. Graphics card** | Checks your NVIDIA card and driver | If it says the driver is old, click **Get the NVIDIA driver**, install the **Game Ready** or **Studio** driver, then click **Check again** |
| **3. Tailscale** | Installs Tailscale, the private link to your Mac | Click **Do it for me** (say **Yes** if Windows asks), then **Open Tailscale to sign in**. Remember which account you used |
| **4. Tools** | Installs FFmpeg (video), Python, Syncthing (file sync) and the PC Bridge helper | Click **Install everything** and wait a few minutes |
| **5. Worker Claude** *(optional)* | Installs Claude Code on the PC so your Mac's Claude can hand it whole tasks | Click **Do it for me**, then **Log in** and finish the login in your browser |
| **6. Optional AI tools** *(optional)* | Ollama (local AI chat models) and ComfyUI (image generation) | Click **Install Ollama** or **Open the ComfyUI download page** if you want them, or skip |
| **7. Wake-on-LAN** *(optional)* | Lets your Mac wake the PC from sleep | Click **Do it for me** and say **Yes** to the Windows admin prompt. If it says the BIOS setting is off, follow the picture, or skip |
| **8. Power** *(optional)* | The PC can sleep when idle, wakes for jobs, and PC Bridge starts with Windows | Click **Do it for me** |
| **9. Pair** | Shows a **6-digit code** | Leave this screen open and go to your Mac |

### About Wake-on-LAN and the BIOS (step 7)

Some PCs also need a switch turned on in the **BIOS**, the settings screen that appears before Windows starts. You can skip this; PC Bridge still works whenever the PC is awake. If you'd like to try it:

1. Restart the PC and press **Delete** or **F2** repeatedly as it starts. The key is shown on screen for a moment.
2. Look under **Advanced → Power Management** (or **APM**) for **Wake on LAN**, **Power On By PCI-E** or **Resume by PCI-E Device**, and set it to **Enabled**. Turn **ErP** off.
3. Press **F10** to save and exit.

---

## Part 4 — The Mac wizard (8 steps)

| Step | What happens | What you do |
|---|---|---|
| **1. Welcome** | Explains what will change on the Mac | Click **Next** |
| **2. Tailscale** | Checks that Tailscale is installed and signed in | Click **Download Tailscale**, install it, and sign in with the **same account** as on the PC. Click **Check again** |
| **3. Pair** | Finds your PC | Choose your PC from the list (or type its name), type the **6-digit code** shown on the PC, and click **Pair** |
| **4. Folders** | Chooses which Mac folders the PC can work on | Click **+ Add folder…** and pick, for example, your Videos or Projects folder. Then click **Share these folders**. PC Bridge sets up syncing for you |
| **5. Connect to Claude** | Gives Claude Code and Claude Desktop the "run on PC" tools | It first **shows exactly what it will add**. Click **Connect to Claude**. Restart Claude Desktop afterwards |
| **6. Speed check** *(optional)* | Measures how fast files move to the PC | Click **Measure my connection**. If you're on Wi-Fi it may suggest Ethernet for big video files |
| **7. Final test** *(optional)* | Runs a real video encode and GPU job on the PC | Click **Run the test** and see how much faster the PC is |
| **8. You're done** | | Click **Finish** |

---

## Part 5 — Using it day to day

* **Just ask Claude.** For example: "encode this video for YouTube", "upscale these photos", "run this training script", "build the project and run the tests". Claude knows which jobs belong on the PC.
* **The menu bar dot** (Mac) or **tray dot** (PC):
  * 🟢 ready
  * 🟡 PC asleep or offline; it will be woken for the next job
  * 🔴 not connected
* **Results come back by themselves** into the folder you were working in. On the PC, a copy stays in `PCBridge\jobs\<date>\` for 14 days.
* **Pause any time:** click the menu bar dot, then **Pause redirecting**. Everything then runs on the Mac as usual.

### Memory (RAM): what PC Bridge does and doesn't do

* PC Bridge **does not add memory to your Mac**. Apps open on your Mac (your browser, editors, Claude itself) always use the **Mac's own memory**.
* What it does is **move heavy jobs away** from the Mac. Jobs sent to the PC use the **PC's 32 GB** and its graphics card.
* It watches how full your Mac's memory is. When the Mac is under pressure, or a job looks like it needs a lot of memory (big pandas/numpy work, Docker, large builds, local AI models), the job goes to the PC.
* The **Status** tab shows Mac and PC free memory side by side, in both apps.

### Big JavaScript builds and Docker (optional)

These go to the PC only in projects you choose. In a project folder that's inside a shared folder, create a file named `.pcbridge` containing:

```
offload = ["npm", "pnpm", "docker"]
```

Remove any of the three you don't want. Quick commands like `npm install` still run on the Mac; builds and test suites go to the PC. **Settings → Docker & npm/pnpm** has a Copy button for this line.

---

## Troubleshooting

Start with **Help → Run diagnostics**. It checks every part and tells you, in plain words, what to fix.

| Problem | Fix |
|---|---|
| Mac says "Can't reach PC Bridge" | Is the PC on, with PC Bridge running (dot in the tray, near the clock)? Is Tailscale connected on **both** computers with the **same account**? |
| Pairing code rejected | Codes expire after 15 minutes and allow 5 tries. On the PC, click **Get a new code** |
| Results don't appear on the Mac | Wait a few seconds; big files take time to sync. Check the file is inside a folder you shared in wizard step 4 |
| PC doesn't wake up | Wake-on-LAN only works when the Mac is on the **same home network** as the PC. Check the BIOS setting (Part 3) |
| A job ran on the Mac instead | The PC wasn't reachable, or PC Bridge estimated the Mac would be faster (for example, a huge file over slow Wi-Fi). You'll see a notification. The **Jobs** tab shows where each job ran |
| "Windows protected your PC" / "Apple cannot check it" | See Part 2 |
| Want to undo everything on the Mac | **Settings → Disconnect from Claude**. This removes the Claude tools, the CLAUDE.md note and the PATH entry. A `.pcbridge-backup` copy of each file from before the change is kept next to it |
| Want to remove it from the PC | Windows **Settings → Apps → PC Bridge → Uninstall**, then delete the `PCBridge` folder if you don't need the job outputs |

## What PC Bridge changes on each computer

**Mac:**
* `~/.pcbridge/` (settings, history, helper commands)
* A "PC Bridge" entry in `~/.claude.json` and in the Claude Desktop config, each backed up first
* A clearly marked block in `~/.claude/CLAUDE.md` and in `~/.zshrc`
* The pairing key, stored in your Keychain

**PC:**
* Everything goes in the `PCBridge` folder: `workspace`, `jobs`, `worker`, `cache`, `logs`, `config`
* Programs installed through Windows' own installer (winget): Tailscale, FFmpeg, Python, Syncthing, and optionally Ollama and Claude Code
* An autostart entry for PC Bridge
* If you allowed step 7: Wake-on-LAN enabled on the network adapter, and Windows "Fast startup" turned off

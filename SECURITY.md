# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for security problems. Use GitHub's private reporting instead: **Security → Report a vulnerability** on this repository. We aim to reply within 7 days.

Please include what an attacker could do, the steps to reproduce, and the versions involved.

## Supported versions

| Version | Supported |
|---|---|
| latest release | ✅ |
| older | ❌ (please upgrade) |

## Security model

PC Bridge lets one Mac run commands on one PC, so the design concentrates on making sure **only your Mac** can do that, and **only inside one folder**.

| Layer | Protection |
|---|---|
| Network | The agent binds only to `127.0.0.1` and the PC's **Tailscale** IP (WireGuard-encrypted, reachable only by devices on your tailnet). It never listens on the LAN or the internet |
| Pairing | A 6-digit code shown on the PC: one use, 15-minute expiry, 5 attempts |
| Authentication | Pairing returns a 256-bit random token. The Mac keeps it in `~/.pcbridge/token` with owner-only permissions (0600; before 0.1.4 it was in the Keychain), and the PC stores only its **SHA-256 hash**. Every endpoint except `/health` and `/pair` requires it |
| File system | Every path is resolved and must be inside the PC root (`D:\PCBridge`). Anything else, including `..` tricks and symlinks out, is refused |
| Execution | Commands run as the normal Windows user, not admin. An optional `allowlist` in `config\settings.json` limits which programs may run |
| Worker Claude | Runs in `PCBridge\worker` with an explicit `--allowedTools` list |
| Program installs | `POST /programs/install` (used by `pc_install` and **Settings → Programs**) takes only a program **name**. The agent looks it up in a fixed catalog (FFmpeg, Blender, PyTorch, Ollama, Node.js, Docker Desktop, Git) and runs that entry's own command: `winget install --id <fixed id> -e` or `pip install` of fixed package names from the official PyTorch index. Unknown names are refused (404). Nothing from the request becomes part of the command. Installs need the token, run as the normal user in the job queue, and go to the programs' normal Windows locations (such as `C:\Program Files` or Python's own folder), not the PCBridge folder or the shared folders. Claude's tool description tells it to ask the user first. Any admin step shows Windows' own "Yes" prompt on the PC |
| Mac app location | At start, a release build of the Mac app may remove macOS's "downloaded from the internet" mark (`com.apple.quarantine`), but **only from its own bundle, `/Applications/PC Bridge.app`**, and only once you've opened PC Bridge (so macOS's own check already ran). It never touches other files or Gatekeeper settings |
| Local admin | The PC's own tray app uses a separate local token file that is never sent over the network |

### Things to know

* Anyone who controls your **Mac user account** can use PC Bridge, the same as they could use anything else on that account.
* Anyone on your **tailnet** can reach the agent's port, but can't use it without a token. Keep your tailnet to your own devices, or use Tailscale ACLs.
* The apps are **not signed with a paid certificate**. The Mac app has an ad-hoc signature (from 0.1.3), which proves the files weren't changed after the build but not who built them. Download the apps only from this repository's Releases page. Automatic updates are separately signed with the project's update key and refused if the signature doesn't match.
* The `allowlist` in `settings.json` applies to `/run` jobs. Program installs are limited by the catalog instead.

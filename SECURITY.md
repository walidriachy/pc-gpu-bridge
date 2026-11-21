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
| Authentication | Pairing returns a 256-bit random token. The Mac keeps it in the **Keychain**, and the PC stores only its **SHA-256 hash**. Every endpoint except `/health` and `/pair` requires it |
| File system | Every path is resolved and must be inside the PC root (`D:\PCBridge`). Anything else, including `..` tricks and symlinks out, is refused |
| Execution | Commands run as the normal Windows user, not admin. An optional `allowlist` in `config\settings.json` limits which programs may run |
| Worker Claude | Runs in `PCBridge\worker` with an explicit `--allowedTools` list |
| Local admin | The PC's own tray app uses a separate local token file that is never sent over the network |

### Things to know

* Anyone who controls your **Mac user account** can use PC Bridge, the same as they could use anything else on that account.
* Anyone on your **tailnet** can reach the agent's port, but can't use it without a token. Keep your tailnet to your own devices, or use Tailscale ACLs.
* The apps are **not code-signed**. Download them only from this repository's Releases page.

"""Run the agent: ``python -m pcbridge_agent [--root D:\\PCBridge] [--port 8765]``.

Listens on 127.0.0.1 (for the PC's own tray app) and on the Tailscale IP only — never on the
home Wi-Fi/LAN address — so only your own devices on your Tailscale network can reach it.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import shutil
import subprocess
import sys
from logging.handlers import RotatingFileHandler
from pathlib import Path

import uvicorn

from .app import create_app
from .config import Settings, default_root


def tailscale_ip() -> str | None:
    candidates = [shutil.which("tailscale"), r"C:\Program Files\Tailscale\tailscale.exe"]
    for exe in filter(None, candidates):
        try:
            out = subprocess.run([exe, "ip", "-4"], capture_output=True, text=True, timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            continue
        ip = out.stdout.strip().splitlines()[0] if out.stdout.strip() else ""
        if ip.startswith("100."):
            return ip
    return None


async def serve(app, hosts: list[str], port: int) -> None:
    servers = [uvicorn.Server(uvicorn.Config(app, host=h, port=port, log_level="info")) for h in hosts]
    for s in servers[1:]:
        s.config.lifespan = "off"  # startup tasks run once
    await asyncio.gather(*(s.serve() for s in servers))


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="pcbridge-agent")
    p.add_argument("--root", type=Path, default=None)
    p.add_argument("--port", type=int, default=None)
    p.add_argument("--host", action="append", default=None, help="override listen address(es)")
    p.add_argument("--print-code", action="store_true", help="print the pairing code and exit")
    args = p.parse_args(argv)

    settings = Settings.load(args.root or default_root())
    if args.port:
        settings.port = args.port
    logs = settings.root_path / "logs"
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        handlers=[
            RotatingFileHandler(logs / "agent.log", maxBytes=5_000_000, backupCount=3),
            logging.StreamHandler(sys.stdout),
        ],
    )
    app = create_app(settings)
    if args.print_code:
        print(app.state.pairing.ensure_code())
        return 0

    hosts = args.host or settings.bind_hosts or ["127.0.0.1"]
    if not (args.host or settings.bind_hosts):
        ts = tailscale_ip()
        if ts:
            hosts.append(ts)
        else:
            logging.warning("Tailscale IP not found: only reachable from this PC until Tailscale is on.")
    logging.info("PC Bridge agent root=%s listening on %s:%s", settings.root_path, hosts, settings.port)
    asyncio.run(serve(app, hosts, settings.port))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""GPU detection through ``nvidia-smi``."""

from __future__ import annotations

import subprocess

from .config import which

QUERY = "name,utilization.gpu,memory.used,memory.total,temperature.gpu,driver_version"


def parse_nvidia_smi(output: str) -> list[dict]:
    """Parse ``nvidia-smi --query-gpu=... --format=csv,noheader,nounits`` output."""
    gpus = []
    for line in output.strip().splitlines():
        parts = [p.strip() for p in line.split(",")]
        if len(parts) < 6:
            continue

        def num(v: str) -> float | None:
            try:
                return float(v)
            except ValueError:
                return None

        gpus.append(
            {
                "name": parts[0],
                "utilization": num(parts[1]),
                "vram_used_mb": num(parts[2]),
                "vram_total_mb": num(parts[3]),
                "temperature_c": num(parts[4]),
                "driver": parts[5],
            }
        )
    return gpus


def driver_is_recent(driver: str, minimum: float = 550.0) -> bool:
    try:
        return float(driver.split(".")[0] + "." + driver.split(".")[1]) >= minimum
    except (IndexError, ValueError):
        return False


def query_gpus(cmd: str = "nvidia-smi") -> dict:
    exe = which(cmd) or cmd
    try:
        out = subprocess.run(
            [exe, f"--query-gpu={QUERY}", "--format=csv,noheader,nounits"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (FileNotFoundError, OSError, subprocess.TimeoutExpired) as exc:
        return {"available": False, "gpus": [], "error": f"nvidia-smi not found or failed: {exc}"}
    if out.returncode != 0:
        return {"available": False, "gpus": [], "error": out.stderr.strip() or "nvidia-smi failed"}
    gpus = parse_nvidia_smi(out.stdout)
    return {
        "available": bool(gpus),
        "gpus": gpus,
        "driver_ok": all(driver_is_recent(g["driver"]) for g in gpus) if gpus else False,
    }

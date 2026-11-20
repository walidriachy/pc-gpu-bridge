"""Delete job and worker folders older than N days."""

from __future__ import annotations

import datetime as dt
import shutil
import time
from pathlib import Path


def cleanup_old(root: Path, days: int, today: dt.date | None = None) -> list[str]:
    removed: list[str] = []
    if days <= 0:
        return removed
    today = today or dt.date.today()
    cutoff = today - dt.timedelta(days=days)
    jobs = Path(root) / "jobs"
    if jobs.exists():
        for day_dir in jobs.iterdir():
            try:
                day = dt.date.fromisoformat(day_dir.name)
            except ValueError:
                continue
            if day < cutoff:
                shutil.rmtree(day_dir, ignore_errors=True)
                removed.append(str(day_dir))
    worker = Path(root) / "worker"
    limit = time.time() - days * 86400
    if worker.exists():
        for d in worker.iterdir():
            if d.is_dir() and d.stat().st_mtime < limit:
                shutil.rmtree(d, ignore_errors=True)
                removed.append(str(d))
    return removed

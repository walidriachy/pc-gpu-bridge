"""Talk to the PC's own Syncthing (REST API on 127.0.0.1) to share folders with the Mac.

The Windows app starts Syncthing with ``--gui-address=127.0.0.1:8385 --gui-apikey=<key>`` and a home
folder inside ``PCBridge\\config\\syncthing``; the key is stored in settings.json.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from pathlib import Path

DEFAULT_IGNORES = [
    "node_modules",
    ".next",
    ".venv",
    "__pycache__",
    "(?d).DS_Store",
    "*.pcbridge-part",
    ".git",  # two computers writing one Git repo at once can corrupt it
    ".claude",  # each computer's own Claude Code settings and session state
]

# Overwritten or deleted files go to the folder's .stversions bin for 14 days instead of vanishing.
VERSIONING = {"type": "trashcan", "params": {"cleanoutDays": "14"}}
# Seconds Syncthing waits after a change before sending it (its default is 10).
WATCH_DELAY_S = 2
# Full rescans are only a safety net (the file watcher reports changes as they happen). Every
# minute on a 100+ GB folder kept the PC re-checking files instead of copying new ones.
RESCAN_S = 3600
# Code and scripts (small) reach the PC before big videos, so jobs can start sooner.
PULL_ORDER = "smallestFirst"


class SyncthingError(RuntimeError):
    pass


class Syncthing:
    def __init__(self, api_key: str, base: str = "http://127.0.0.1:8385", opener=None):
        self.base = base.rstrip("/")
        self.key = api_key
        self._open = opener or urllib.request.urlopen

    def _call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            self.base + path,
            data=data,
            method=method,
            headers={"X-API-Key": self.key, "Content-Type": "application/json"},
        )
        try:
            with self._open(req, timeout=10) as r:
                raw = r.read().decode()
                return json.loads(raw) if raw.strip() else None
        except (urllib.error.URLError, OSError) as exc:
            raise SyncthingError(f"Syncthing isn't responding on the PC: {exc}") from exc

    def device_id(self) -> str:
        return self._call("GET", "/rest/system/status")["myID"]

    def add_device(self, device_id: str, name: str, address: str = "dynamic") -> None:
        self._call(
            "PUT",
            f"/rest/config/devices/{device_id}",
            {
                "deviceID": device_id,
                "name": name,
                "addresses": [address],
                "autoAcceptFolders": False,
            },
        )

    def share_folder(self, folder_id: str, label: str, path: Path, devices: list[str]) -> None:
        path.mkdir(parents=True, exist_ok=True)
        self._call(
            "PUT",
            f"/rest/config/folders/{folder_id}",
            {
                "id": folder_id,
                "label": label,
                "path": str(path),
                "type": "sendreceive",
                "devices": [{"deviceID": d} for d in devices],
                "fsWatcherEnabled": True,
                "fsWatcherDelayS": WATCH_DELAY_S,
                "rescanIntervalS": RESCAN_S,
                "order": PULL_ORDER,
                # Windows + many small files: syncing each file to disk one by one is very slow,
                # and Syncthing re-checks anything lost in a crash anyway.
                "disableFsync": True,
                "versioning": VERSIONING,
            },
        )
        self._call("POST", f"/rest/db/ignores?folder={folder_id}", {"ignore": DEFAULT_IGNORES})


def folder_id(name: str) -> str:
    safe = "".join(c if c.isalnum() else "-" for c in name.lower()).strip("-")
    return f"pcbridge-{safe or 'shared'}"

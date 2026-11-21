"""6-digit pairing code -> long random token.

The Windows app reads ``config/pairing.json`` to show the code on screen. The Mac app sends the
code to ``POST /pair`` and receives a token it keeps in a private file. Only a SHA-256 hash of
each token is stored on the PC. Each Mac sends a stable ``device_id``, so pairing the same Mac again
replaces its old entry (and its old token stops working) instead of adding a duplicate.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import secrets
import time
from pathlib import Path

CODE_TTL_SECONDS = 15 * 60
MAX_ATTEMPTS = 5


def _hash(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


class PairingStore:
    def __init__(self, config_dir: Path, clock=time.time):
        self.dir = Path(config_dir)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.clock = clock
        self.auth_file = self.dir / "auth.json"
        self.code_file = self.dir / "pairing.json"
        self.local_token_file = self.dir / "local_token"

    # ---- tokens --------------------------------------------------------
    def _auth(self) -> dict:
        if self.auth_file.exists():
            return json.loads(self.auth_file.read_text("utf-8"))
        return {"devices": []}

    def _save_auth(self, data: dict) -> None:
        self.auth_file.write_text(json.dumps(data, indent=2), "utf-8")

    def local_token(self) -> str:
        """Token for the PC's own tray app (same user, local file)."""
        if not self.local_token_file.exists():
            self.local_token_file.write_text(secrets.token_urlsafe(32), "utf-8")
        return self.local_token_file.read_text("utf-8").strip()

    def verify(self, token: str | None) -> bool:
        if not token:
            return False
        if hmac.compare_digest(token, self.local_token()):
            return True
        h = _hash(token)
        return any(hmac.compare_digest(h, d["hash"]) for d in self._auth()["devices"])

    def is_local(self, token: str | None) -> bool:
        return bool(token) and hmac.compare_digest(token, self.local_token())

    def _index_of(self, devices: list[dict], token: str | None) -> int | None:
        if not token:
            return None
        h = _hash(token)
        for i, d in enumerate(devices):
            if hmac.compare_digest(h, d["hash"]):
                return i
        return None

    def devices(self) -> list[dict]:
        return [
            {"index": i, "name": d["name"], "paired_at": d["paired_at"], "device_id": d.get("device_id")}
            for i, d in enumerate(self._auth()["devices"])
        ]

    def forget(self, token: str | None) -> bool:
        """Remove the device that owns ``token`` (a Mac unpairing itself)."""
        auth = self._auth()
        i = self._index_of(auth["devices"], token)
        if i is None:
            return False
        del auth["devices"][i]
        self._save_auth(auth)
        return True

    def remove(self, index: int) -> bool:
        """Remove a device by its position in ``devices()`` (the PC's own app)."""
        auth = self._auth()
        if not 0 <= index < len(auth["devices"]):
            return False
        del auth["devices"][index]
        self._save_auth(auth)
        return True

    def claim(self, token: str | None, device_id: str, device_name: str) -> int:
        """Tag the caller's entry with ``device_id`` and drop stale duplicates of the same Mac.

        Before 0.1.4, pairing again added a new entry each time. Entries with the same name and no
        device_id are older pairings of this Mac; their tokens are no longer used. Returns how many
        entries were removed.
        """
        device_id = (device_id or "").strip()[:64]
        if not device_id:
            return 0
        auth = self._auth()
        devices = auth["devices"]
        i = self._index_of(devices, token)
        if i is None:
            return 0
        me = devices[i]
        me["device_id"] = device_id
        keep = [
            d
            for d in devices
            if d is me
            or not (d.get("device_id") == device_id or (not d.get("device_id") and d["name"] == me["name"]))
        ]
        removed = len(devices) - len(keep)
        auth["devices"] = keep
        self._save_auth(auth)
        return removed

    def unpair_all(self) -> None:
        self._save_auth({"devices": []})

    # ---- codes ---------------------------------------------------------
    def new_code(self) -> str:
        code = f"{secrets.randbelow(1_000_000):06d}"
        self.code_file.write_text(
            json.dumps({"code": code, "expires": self.clock() + CODE_TTL_SECONDS, "attempts": 0}),
            "utf-8",
        )
        return code

    def current_code(self) -> dict | None:
        if not self.code_file.exists():
            return None
        data = json.loads(self.code_file.read_text("utf-8"))
        if data["expires"] < self.clock() or data["attempts"] >= MAX_ATTEMPTS:
            return None
        return data

    def ensure_code(self) -> str:
        cur = self.current_code()
        return cur["code"] if cur else self.new_code()

    def redeem(self, code: str, device_name: str, device_id: str | None = None) -> str | None:
        """Return a new token if ``code`` is valid, else None (and count the attempt)."""
        cur = self.current_code()
        if cur is None:
            return None
        if not hmac.compare_digest(str(code).strip(), cur["code"]):
            cur["attempts"] += 1
            self.code_file.write_text(json.dumps(cur), "utf-8")
            return None
        token = secrets.token_urlsafe(32)
        auth = self._auth()
        name = device_name[:64] or "Mac"
        did = (device_id or "").strip()[:64]
        if did:
            # Same Mac pairing again: replace its old entry(ies), so old tokens stop working.
            auth["devices"] = [
                d
                for d in auth["devices"]
                if d.get("device_id") != did and not (not d.get("device_id") and d["name"] == name)
            ]
        entry = {"name": name, "hash": _hash(token), "paired_at": int(self.clock())}
        if did:
            entry["device_id"] = did
        auth["devices"].append(entry)
        self._save_auth(auth)
        self.code_file.unlink(missing_ok=True)  # one-time use
        self.new_code()  # fresh code ready for another device
        return token

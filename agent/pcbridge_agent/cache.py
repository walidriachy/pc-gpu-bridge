"""Result cache: same command + same input file contents => reuse previous outputs."""

from __future__ import annotations

import hashlib
import json
import shutil
import time
from pathlib import Path


def hash_file(path: Path, chunk: int = 1 << 20) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while block := f.read(chunk):
            h.update(block)
    return h.hexdigest()


def cache_key(argv: list[str], input_files: list[Path]) -> str:
    h = hashlib.sha256()
    h.update(json.dumps(argv).encode())
    for p in sorted(input_files, key=str):
        h.update(str(p.name).encode())
        h.update(hash_file(p).encode())
    return h.hexdigest()


class ResultCache:
    def __init__(self, cache_dir: Path, root: Path, max_bytes: int):
        self.dir = Path(cache_dir)
        self.root = Path(root).resolve()
        self.max_bytes = max_bytes
        self.dir.mkdir(parents=True, exist_ok=True)

    def _entry(self, key: str) -> Path:
        return self.dir / key

    def get(self, key: str) -> list[str] | None:
        """Restore cached outputs into place; return list of restored relative paths."""
        entry = self._entry(key)
        manifest = entry / "manifest.json"
        if not manifest.exists():
            return None
        data = json.loads(manifest.read_text("utf-8"))
        restored = []
        for rel in data["outputs"]:
            src = entry / "files" / rel
            if not src.exists():
                return None
            dst = self.root / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
            restored.append(rel)
        data["last_used"] = time.time()
        manifest.write_text(json.dumps(data), "utf-8")
        return restored

    def put(self, key: str, outputs: list[Path], exit_code: int) -> None:
        if exit_code != 0 or not outputs:
            return
        entry = self._entry(key)
        files = entry / "files"
        rels = []
        for p in outputs:
            rel = Path(p).resolve().relative_to(self.root).as_posix()
            dst = files / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(p, dst)
            rels.append(rel)
        (entry / "manifest.json").write_text(
            json.dumps({"outputs": rels, "created": time.time(), "last_used": time.time()}), "utf-8"
        )
        self.enforce_cap()

    def size(self) -> int:
        return sum(f.stat().st_size for f in self.dir.rglob("*") if f.is_file())

    def enforce_cap(self) -> None:
        """Evict least-recently-used entries until under the size cap."""
        entries = []
        for e in self.dir.iterdir():
            m = e / "manifest.json"
            if m.exists():
                last = json.loads(m.read_text("utf-8")).get("last_used", 0)
                size = sum(f.stat().st_size for f in e.rglob("*") if f.is_file())
                entries.append((last, size, e))
        total = sum(s for _, s, _ in entries)
        for _, size, e in sorted(entries, key=lambda t: t[0]):
            if total <= self.max_bytes:
                break
            shutil.rmtree(e, ignore_errors=True)
            total -= size

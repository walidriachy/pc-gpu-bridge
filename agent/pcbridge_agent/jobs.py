"""FIFO job queue with a fixed number of parallel slots (default 1 GPU job at a time).

Each job gets its own folder: ``jobs/YYYY-MM-DD/<id>_<short-name>/{input,output,job.json,log.txt}``.
Events are kept in memory so several clients can follow (or re-attach to) the same job.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import json
import os
import re
import shutil
import sys
import time
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from pathlib import Path

from .config import system_path

TERMINAL = {"done", "failed", "cancelled"}


def keep_awake(on: bool) -> None:
    """Stop Windows from sleeping while jobs run; allow sleep again when idle."""
    if sys.platform != "win32":
        return
    import ctypes

    es_continuous, es_system_required = 0x80000000, 0x00000001
    flags = es_continuous | (es_system_required if on else 0)
    ctypes.windll.kernel32.SetThreadExecutionState(flags)  # type: ignore[attr-defined]


def short_name(text: str) -> str:
    s = re.sub(r"[^A-Za-z0-9]+", "-", text).strip("-").lower()
    return (s or "job")[:32]


@dataclass
class Job:
    id: str
    kind: str  # "run" | "delegate"
    name: str
    argv: list[str]
    cwd: Path
    folder: Path
    gpu: bool = True
    env: dict[str, str] = field(default_factory=dict)
    state: str = "queued"
    exit_code: int | None = None
    created: float = field(default_factory=time.time)
    started: float | None = None
    finished: float | None = None
    outputs: list[str] = field(default_factory=list)
    cached: bool = False
    events: list[dict] = field(default_factory=list)
    _changed: asyncio.Event = field(default_factory=asyncio.Event, repr=False)
    _proc: asyncio.subprocess.Process | None = field(default=None, repr=False)
    _cancel: bool = field(default=False, repr=False)

    def emit(self, event: dict) -> None:
        event.setdefault("t", round(time.time(), 3))
        self.events.append(event)
        self._changed.set()
        if event["type"] in ("stdout", "stderr", "claude"):
            with open(self.folder / "log.txt", "a", encoding="utf-8", errors="replace") as f:
                f.write(event.get("data") or json.dumps(event.get("event")) + "\n")

    def summary(self) -> dict:
        return {
            "id": self.id,
            "kind": self.kind,
            "name": self.name,
            "argv": self.argv,
            "state": self.state,
            "exit_code": self.exit_code,
            "created": self.created,
            "started": self.started,
            "finished": self.finished,
            "duration": (self.finished or time.time()) - self.started if self.started else None,
            "outputs": self.outputs,
            "cached": self.cached,
            "folder": str(self.folder),
        }

    def write_meta(self) -> None:
        (self.folder / "job.json").write_text(json.dumps(self.summary(), indent=2), "utf-8")

    async def follow(self, start: int = 0) -> AsyncIterator[dict]:
        """Yield every event from ``start`` until the job finishes."""
        i = start
        while True:
            while i < len(self.events):
                yield self.events[i]
                i += 1
            if self.state in TERMINAL:
                return
            self._changed.clear()
            if i < len(self.events):
                continue
            await self._changed.wait()


# Folders that never hold a job's results but can contain hundreds of thousands of files.
SKIP_DIRS = {
    "node_modules",
    ".git",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".stfolder",
    ".stversions",
    ".cache",
    ".turbo",
    "dist-newstyle",
    "target",
    ".gradle",
    ".pnpm-store",
}
SNAPSHOT_MAX_FILES = 200_000


def _is_sync_temp(name: str) -> bool:
    """Syncthing's in-progress files (and PC Bridge's own partial downloads)."""
    return name.startswith((".syncthing.", "~syncthing~")) or name.endswith((".tmp", ".pcbridge-part"))


def snapshot(folder: Path) -> dict[str, int]:
    """Map of file -> mtime for detecting what a job created or changed.

    Walks with ``os.scandir`` (fast on Windows), skips dependency/VCS folders and Syncthing temp
    files, and stops after :data:`SNAPSHOT_MAX_FILES` files. Run it in a worker thread: on a big
    shared folder it can take seconds, and the agent must keep answering meanwhile.
    """
    out: dict[str, int] = {}
    stack = [str(folder)]
    while stack and len(out) < SNAPSHOT_MAX_FILES:
        try:
            with os.scandir(stack.pop()) as it:
                for entry in it:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            if entry.name not in SKIP_DIRS:
                                stack.append(entry.path)
                        elif entry.is_file(follow_symlinks=False) and not _is_sync_temp(entry.name):
                            out[entry.path] = entry.stat().st_mtime_ns
                    except OSError:
                        continue
        except OSError:
            continue
    return out


def changed_files(before: dict[str, int], after: dict[str, int], started: float) -> list[Path]:
    """Files the job created or modified.

    Files Syncthing brings in from the Mac while the job runs keep their old (Mac) modification
    time, so only files modified after the job started count.
    """
    since = int((started - 2) * 1e9)
    return [Path(p) for p, m in after.items() if before.get(p) != m and m >= since]


class JobQueue:
    def __init__(self, root: Path, max_parallel: int = 1):
        self.root = Path(root).resolve()
        self.max_parallel = max_parallel
        self.jobs: dict[str, Job] = {}
        self.waiting: list[str] = []
        self.running: set[str] = set()
        self._cond: asyncio.Condition | None = None
        self.on_finish = None  # optional callback(job, outputs: list[Path])

    @property
    def cond(self) -> asyncio.Condition:
        if self._cond is None:
            self._cond = asyncio.Condition()
        return self._cond

    def create(
        self, kind: str, name: str, argv: list[str], cwd: Path, gpu: bool = True, env: dict | None = None
    ) -> Job:
        jid = uuid.uuid4().hex[:10]
        day = dt.date.today().isoformat()
        folder = self.root / "jobs" / day / f"{jid}_{short_name(name)}"
        (folder / "input").mkdir(parents=True, exist_ok=True)
        (folder / "output").mkdir(parents=True, exist_ok=True)
        job = Job(id=jid, kind=kind, name=name, argv=argv, cwd=cwd, folder=folder, gpu=gpu, env=env or {})
        self.jobs[jid] = job
        job.write_meta()
        job.emit({"type": "job", "id": jid, "folder": str(folder)})
        return job

    def position(self, jid: str) -> int:
        return self.waiting.index(jid) + 1 if jid in self.waiting else 0

    def queue_depth(self) -> int:
        return len(self.waiting) + len(self.running)

    async def submit(self, job: Job) -> None:
        """Start the job in the background; returns immediately."""
        self.waiting.append(job.id)
        job.emit({"type": "queued", "position": self.position(job.id)})
        asyncio.get_running_loop().create_task(self._run(job))

    async def _acquire(self, job: Job) -> bool:
        async with self.cond:
            while True:
                if job._cancel:
                    if job.id in self.waiting:
                        self.waiting.remove(job.id)
                    return False
                if self.waiting and self.waiting[0] == job.id and len(self.running) < self.max_parallel:
                    self.waiting.pop(0)
                    self.running.add(job.id)
                    keep_awake(True)
                    self.cond.notify_all()
                    return True
                await self.cond.wait()

    async def _release(self, job: Job) -> None:
        async with self.cond:
            self.running.discard(job.id)
            if not self.running:
                keep_awake(False)
            self.cond.notify_all()

    async def _run(self, job: Job) -> None:
        if not await self._acquire(job):
            job.state = "cancelled"
            job.finished = time.time()
            job.emit({"type": "exit", "code": -1, "state": "cancelled"})
            job.write_meta()
            return
        # Keep the slot until outputs are collected, so the next job can't mix its files in.
        try:
            before = await asyncio.to_thread(snapshot, job.cwd)
            job.state = "running"
            job.started = time.time()
            job.emit({"type": "started"})
            code = -1
            try:
                fresh = system_path()
                env = {**os.environ, "PATH": fresh, **job.env}
                # Resolve the program ourselves: on Windows the child's env PATH isn't used to find it.
                program = shutil.which(job.argv[0], path=env.get("PATH", fresh)) or job.argv[0]
                job._proc = await asyncio.create_subprocess_exec(
                    program,
                    *job.argv[1:],
                    cwd=str(job.cwd),
                    env=env,
                    stdin=asyncio.subprocess.DEVNULL,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                await asyncio.gather(
                    self._pump(job, job._proc.stdout, "stdout"),
                    self._pump(job, job._proc.stderr, "stderr"),
                )
                code = await job._proc.wait()
            except FileNotFoundError:
                job.emit(
                    {"type": "stderr", "data": f"PC Bridge: program not found on the PC: {job.argv[0]}\n"}
                )
                code = 127
            except Exception as exc:  # noqa: BLE001 - report everything to the client
                job.emit({"type": "stderr", "data": f"PC Bridge: failed to start job: {exc}\n"})
                code = 1

            after = await asyncio.to_thread(snapshot, job.cwd)
            changed = changed_files(before, after, job.started)
            # Copying outputs (possibly GBs of video) happens off the event loop too.
            job.outputs.extend(await asyncio.to_thread(self._collect_outputs, changed, job.folder / "output"))
            job.exit_code = code
            job.finished = time.time()
            job.state = "cancelled" if job._cancel else ("done" if code == 0 else "failed")
            if self.on_finish:
                try:
                    await asyncio.to_thread(self.on_finish, job, changed)
                except Exception:  # noqa: BLE001
                    pass
        finally:
            await self._release(job)
        job.write_meta()
        job.emit(
            {
                "type": "exit",
                "code": code,
                "state": job.state,
                "duration": round(job.finished - job.started, 3),
                "outputs": job.outputs,
            }
        )

    def _collect_outputs(self, changed: list[Path], out_dir: Path) -> list[str]:
        rels = []
        for p in changed:
            try:
                rel = p.resolve().relative_to(self.root).as_posix()
            except ValueError:
                continue
            rels.append(rel)
            try:
                if out_dir not in p.parents:
                    shutil.copy2(p, out_dir / p.name)
            except OSError:
                pass
        return rels

    async def _pump(self, job: Job, stream: asyncio.StreamReader | None, kind: str) -> None:
        if stream is None:
            return
        if job.kind == "delegate" and kind == "stdout":
            # Claude stream-json: one JSON object per line.
            async for raw in stream:
                line = raw.decode("utf-8", "replace").strip()
                if not line:
                    continue
                try:
                    job.emit({"type": "claude", "event": json.loads(line)})
                except json.JSONDecodeError:
                    job.emit({"type": "stdout", "data": line + "\n"})
            return
        while chunk := await stream.read(4096):
            job.emit({"type": kind, "data": chunk.decode("utf-8", "replace")})

    async def cancel(self, jid: str) -> bool:
        job = self.jobs.get(jid)
        if not job or job.state in TERMINAL:
            return False
        job._cancel = True
        if job._proc and job._proc.returncode is None:
            try:
                job._proc.kill()
            except ProcessLookupError:
                pass
        async with self.cond:
            self.cond.notify_all()
        return True

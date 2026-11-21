"""FIFO job queue with a fixed number of parallel slots (default 1 GPU job at a time).

Program installs have their own lane (one at a time) so a long download, or a Windows "Yes"
prompt nobody is there to click, never holds up the jobs sent from the Mac, and vice versa.

Each job gets its own folder: ``jobs/YYYY-MM-DD/<id>_<short-name>/{input,output,job.json,log.txt}``.
Events are kept in memory so several clients can follow (or re-attach to) the same job.
"""

from __future__ import annotations

import asyncio
import codecs
import datetime as dt
import json
import math
import os
import re
import shutil
import sys
import time
import uuid
from collections import deque
from collections.abc import AsyncIterator, Iterable
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath, PureWindowsPath

from . import programs
from .config import looks_absolute, system_path

TERMINAL = {"done", "failed", "cancelled"}

# Installs: the longest one may take, and (winget only) how long it may show no progress before
# PC Bridge says Windows may be waiting for a "Yes" click, then gives up. pip prints nothing while
# it downloads PyTorch, so only the overall limit applies to it.
INSTALL_TIME_LIMIT_S = 3 * 3600.0
WINGET_QUIET_NOTICE_S = 90.0
WINGET_QUIET_LIMIT_S = 20 * 60.0
WATCH_INTERVAL_S = 5.0


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
    kind: str  # "run" | "delegate" | "install" | "comfy"
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
    _missing: str = field(default="", repr=False)  # catalog key of a program the job needed
    # A module the job's own Python (a venv, conda…, not the PC's main one) lacks: catalog key.
    _other_python: str = field(default="", repr=False)
    _pc_python: bool = field(default=True, repr=False)  # "No module named" means the PC's Python
    _program: str = field(default="", repr=False)  # the program actually started
    _restart: bool = field(default=False, repr=False)  # installed; the PC must restart to finish
    _last_progress: float = field(default=0.0, repr=False)  # last output with real text in it

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
            "missing": self._missing or None,
            "restart_needed": self._restart,
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
SCAN_DEPTH = 2  # levels of subfolders looked at below the job's folder (and folders it names)
SNAPSHOT_BUDGET_S = 5.0  # time limit for one scan, so a huge shared folder can't stall a job
MAX_ARG_FOLDERS = 32


def _is_sync_temp(name: str) -> bool:
    """Syncthing's in-progress files (and PC Bridge's own partial downloads)."""
    return name.startswith((".syncthing.", "~syncthing~")) or name.endswith((".tmp", ".pcbridge-part"))


def _arg_folder(cwd: Path, text: str, root: Path) -> tuple[Path, int] | None:
    """The folder a command-line argument points at, if it is a path inside the PCBridge root.

    A folder is scanned like the job's own folder; for a file (existing or not yet written, such
    as ``out.mp4`` or ``frames/f_%04d.png``) only its parent folder's own files are looked at.
    """
    if not text or len(text) > 1024 or "\n" in text or "\0" in text or "://" in text:
        return None
    if text.startswith(("-", "//")):  # flags; Blender's "//" means "next to the .blend file"
        return None
    if looks_absolute(text):
        candidate = Path(text)
    else:
        parts = PureWindowsPath(text).parts if "\\" in text else PurePosixPath(text).parts
        candidate = cwd.joinpath(*parts)
    try:
        resolved = candidate.resolve()
        if not resolved.is_relative_to(root):
            return None
        if resolved.is_dir():
            return resolved, SCAN_DEPTH
        if resolved.parent.is_dir():
            return resolved.parent, 0
    except (OSError, ValueError):
        pass
    return None


def _blend_folder(cwd: Path, argv: list[str], root: Path) -> Path:
    """The folder of the job's first ``.blend`` file (what Blender's ``//`` means), else ``cwd``."""
    for arg in argv[1:]:
        if arg.lower().endswith(".blend"):
            found = _arg_folder(cwd, arg, root)
            if found:
                return found[0]
    return cwd


def scan_targets(cwd: Path, argv: list[str], root: Path) -> list[tuple[Path, int]]:
    """Folders named by the job's arguments (absolute or relative to ``cwd``) inside ``root``.

    Blender's ``//renders/f_####`` is relative to the ``.blend`` file: that file's folder is
    scanned like the job's own folder (the renders folder may not exist before the job runs).
    """
    root = Path(root).resolve()
    cwd = Path(cwd).resolve()
    out: list[tuple[Path, int]] = []
    blend: Path | None = None

    def add(found: tuple[Path, int] | None) -> None:
        # The job's own folder is always scanned anyway.
        if found and found[0] != cwd and found not in out:
            out.append(found)

    for arg in argv[1:]:
        for piece in dict.fromkeys((arg, arg.split("=", 1)[-1])):  # also "--out=renders/x.png"
            if piece.startswith("//") and "://" not in piece:
                blend = blend or _blend_folder(cwd, argv, root)
                add(_arg_folder(blend, str(blend), root))
                if piece.strip("/"):
                    add(_arg_folder(blend, piece.lstrip("/"), root))
            else:
                add(_arg_folder(cwd, piece, root))
        if len(out) >= MAX_ARG_FOLDERS:
            break
    return out


def snapshot(
    folder: Path,
    extra: Iterable[tuple[Path, int | None]] = (),
    depth: int | None = SCAN_DEPTH,
    budget: float = SNAPSHOT_BUDGET_S,
) -> dict[str, int]:
    """Map of file -> mtime for detecting what a job created or changed.

    Looks at ``folder`` (the job's working folder) down to ``depth`` levels of subfolders
    (``None`` = all), plus each ``(folder, depth)`` in ``extra`` (the folders the job's arguments
    name, see :func:`scan_targets`), nearest folders first. It skips dependency/VCS folders and
    Syncthing temp files, and stops after :data:`SNAPSHOT_MAX_FILES` files or ``budget`` seconds.

    So on a huge shared folder a job only looks where its results are likely to be. Files a job
    writes elsewhere aren't listed as its outputs, but Syncthing still brings them to the Mac.
    Uses ``os.scandir`` (fast on Windows); run it in a worker thread so the agent keeps answering.
    """
    deadline = time.monotonic() + budget
    out: dict[str, int] = {}
    todo: deque[tuple[str, float]] = deque()
    seen: dict[str, float] = {}

    def add(path: str | Path, levels: float | None) -> None:
        levels = math.inf if levels is None else levels
        key = os.path.normcase(os.path.abspath(path))
        if seen.get(key, -1) >= levels:
            return
        seen[key] = levels
        todo.append((str(path), levels))

    for path, levels in extra:
        add(path, levels)
    add(folder, depth)
    scanned = 0
    while todo and len(out) < SNAPSHOT_MAX_FILES and time.monotonic() < deadline:
        path, levels = todo.popleft()
        try:
            with os.scandir(path) as it:
                for entry in it:
                    scanned += 1
                    if scanned % 2048 == 0 and time.monotonic() >= deadline:
                        break
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            if levels > 0 and entry.name not in SKIP_DIRS:
                                add(entry.path, levels - 1)
                        elif entry.is_file(follow_symlinks=False) and not _is_sync_temp(entry.name):
                            out[entry.path] = entry.stat().st_mtime_ns
                    except OSError:
                        continue
        except OSError:
            continue
    return out


def _scan(job: Job, root: Path) -> dict[str, int]:
    """Snapshot of the places a job's results can be (blocking: run it in a worker thread)."""
    # A delegated Claude worker has its own fresh folder: look at all of it.
    depth = None if job.kind == "delegate" else SCAN_DEPTH
    cwd = Path(job.cwd).resolve()  # same spelling as the argument folders, so no file counts twice
    return snapshot(cwd, scan_targets(cwd, job.argv, root), depth)


def _resolve_program(argv0: str, path: str, own_path: bool) -> str:
    """Full path of the program to start.

    On Windows the child's PATH isn't used to find the program, so we look it up ourselves:
    a bare ``python`` is the PC's own Python (the one PC Bridge installs packages into, never
    the agent's private one), and programs whose installer skips PATH (Blender) are found in
    their install folder.
    """
    bare = "/" not in argv0 and "\\" not in argv0
    if bare and not own_path and argv0.lower().removesuffix(".exe") in ("python", "python3"):
        py = programs.pc_python()
        if py:
            return py
    return shutil.which(argv0, path=path) or (programs.find_command(argv0) if bare else None) or argv0


_PYTHONS = ("python", "pythonw", "py", "pyw", "conda", "uv")


def _uses_pc_python(program: str) -> bool:
    """Whether "No module named …" from this program means the PC's own Python lacks it.

    ``pc_install`` only fills the PC's own Python, so a job started with a venv's, conda's or
    another Python (``.venv\\Scripts\\python.exe``, ``py -3.11``) needs a different hint.
    Shells and other programs are assumed to use the PC's Python.
    """
    path = Path(program.replace("\\", "/"))
    py = programs.pc_python()
    if py and os.path.normcase(os.path.abspath(program)) == os.path.normcase(os.path.abspath(py)):
        return True
    name = path.name.lower().removesuffix(".exe")
    if name in _PYTHONS or re.fullmatch(r"python[0-9.]*w?", name):
        return False
    # A virtual environment's own commands (whisper.exe in .venv\Scripts) use that environment.
    return not (path.parent.parent / "pyvenv.cfg").is_file()


def _job_python(program: str) -> str:
    """The Python to name in a pip hint for a job started as ``program``."""
    path = Path(program.replace("\\", "/"))
    name = path.name.lower().removesuffix(".exe")
    if name in _PYTHONS or re.fullmatch(r"python[0-9.]*w?", name):
        return program
    return str(Path(program).parent / ("python.exe" if program.lower().endswith(".exe") else "python"))


def _is_winget(argv: list[str]) -> bool:
    return bool(argv) and Path(argv[0].replace("\\", "/")).name.lower().removesuffix(".exe") == "winget"


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

    @staticmethod
    def lane(job: Job) -> str:
        """Installs wait only for other installs; every other job shares the GPU slots."""
        return "install" if job.kind == "install" else "jobs"

    def _lane_ids(self, ids: Iterable[str], lane: str) -> list[str]:
        return [j for j in ids if j in self.jobs and self.lane(self.jobs[j]) == lane]

    def position(self, jid: str) -> int:
        job = self.jobs.get(jid)
        if not job or jid not in self.waiting:
            return 0
        return self._lane_ids(self.waiting, self.lane(job)).index(jid) + 1

    def queue_depth(self) -> int:
        """Jobs waiting for or using the GPU slots (installs don't count: they don't use the GPU)."""
        return len(self._lane_ids(self.waiting, "jobs")) + len(self._lane_ids(self.running, "jobs"))

    async def submit(self, job: Job) -> None:
        """Start the job in the background; returns immediately."""
        self.waiting.append(job.id)
        job.emit({"type": "queued", "position": self.position(job.id)})
        asyncio.get_running_loop().create_task(self._run(job))

    async def _acquire(self, job: Job) -> bool:
        lane = self.lane(job)
        slots = 1 if lane == "install" else self.max_parallel
        async with self.cond:
            while True:
                if job._cancel:
                    if job.id in self.waiting:
                        self.waiting.remove(job.id)
                    return False
                waiting = self._lane_ids(self.waiting, lane)
                if waiting and waiting[0] == job.id and len(self._lane_ids(self.running, lane)) < slots:
                    self.waiting.remove(job.id)
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
        # Installs write to Program Files, never to the shared folders: nothing to look for.
        install = job.kind == "install"
        try:
            before = {} if install else await asyncio.to_thread(_scan, job, self.root)
            job.state = "running"
            job.started = job._last_progress = time.time()
            job.emit({"type": "started"})
            code = -1
            stopped = ""
            try:
                fresh = system_path()
                env = {**os.environ, "PATH": fresh, **job.env}
                program = _resolve_program(job.argv[0], env.get("PATH", fresh), "PATH" in job.env)
                job._program = program
                job._pc_python = await asyncio.to_thread(_uses_pc_python, program)
                job._proc = await asyncio.create_subprocess_exec(
                    program,
                    *job.argv[1:],
                    cwd=str(job.cwd),
                    env=env,
                    stdin=asyncio.subprocess.DEVNULL,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                watch = asyncio.get_running_loop().create_task(self._watch(job)) if install else None
                try:
                    await asyncio.gather(
                        self._pump(job, job._proc.stdout, "stdout"),
                        self._pump(job, job._proc.stderr, "stderr"),
                    )
                    code = await job._proc.wait()
                finally:
                    if watch:
                        watch.cancel()
                        try:
                            stopped = await watch
                        except asyncio.CancelledError:
                            stopped = ""
            except FileNotFoundError:
                job.emit(
                    {
                        "type": "stderr",
                        "data": f"PC Bridge: program not found on the PC: {job.argv[0]} "
                        "(it isn't installed there, or Windows can't find it)\n",
                    }
                )
                code = 127
                prog = programs.lookup(job.argv[0])
                if prog:
                    job._missing = prog.key
            except Exception as exc:  # noqa: BLE001 - report everything to the client
                job.emit({"type": "stderr", "data": f"PC Bridge: failed to start job: {exc}\n"})
                code = 1

            if stopped:
                job.emit({"type": "stderr", "data": stopped})
            elif install and code != 0:
                code = self._install_result(job, code)
            if job._missing and code != 0:
                prog = programs.CATALOG[job._missing]
                job.emit({"type": "stderr", "data": programs.hint(prog)})
                job.emit({"type": "missing", "program": prog.key, "name": prog.name})
            elif job._other_python and code != 0:
                prog = programs.CATALOG[job._other_python]
                job.emit({"type": "stderr", "data": programs.pip_hint(prog, _job_python(job._program))})
            after = {} if install else await asyncio.to_thread(_scan, job, self.root)
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

    @staticmethod
    def _install_result(job: Job, code: int) -> int:
        """Turn winget's "nothing to do" and "restart the PC" exit codes into plain results."""
        if programs.already_installed_code(code):
            job.emit({"type": "stdout", "data": "PC Bridge: it was already installed, nothing to do.\n"})
            return 0
        if programs.restart_to_finish_code(code):
            job._restart = True
            done = "PC Bridge: installed. Restart the PC to finish the install.\n"
            job.emit({"type": "stdout", "data": done})
            job.emit({"type": "restart_needed"})
            return 0
        if programs.restart_to_install_code(code):
            job.emit(
                {
                    "type": "stderr",
                    "data": "PC Bridge: Windows needs to restart before this can be installed. "
                    "Restart the PC, then install it again.\n",
                }
            )
        return code

    async def _watch(self, job: Job) -> str:
        """Stop an install that runs too long, or (winget) shows no progress for too long.

        Returns the plain message explaining why it was stopped ("" while it keeps running).
        """
        winget = _is_winget(job.argv)
        noticed = False
        while job._proc and job._proc.returncode is None:
            await asyncio.sleep(WATCH_INTERVAL_S)
            now = time.time()
            quiet = now - job._last_progress
            if job.started and now - job.started > INSTALL_TIME_LIMIT_S:
                why = (
                    f"PC Bridge: stopped the install after {INSTALL_TIME_LIMIT_S / 3600:.0f} hours. "
                    "Check the PC's internet connection, then try again.\n"
                )
            elif winget and quiet > WINGET_QUIET_LIMIT_S:
                why = (
                    f"PC Bridge: stopped the install after {WINGET_QUIET_LIMIT_S / 60:.0f} minutes without "
                    "progress. Windows may have been waiting for someone to click “Yes” on the PC's screen. "
                    "Try again when someone is at the PC to click Yes.\n"
                )
            else:
                if winget and not noticed and quiet > WINGET_QUIET_NOTICE_S:
                    noticed = True
                    job.emit(
                        {
                            "type": "stderr",
                            "data": "PC Bridge: still installing… If Windows asks “Do you want to allow this "
                            "app to make changes?” on the PC's screen, someone there needs to click Yes.\n",
                        }
                    )
                continue
            try:
                job._proc.kill()
            except ProcessLookupError:
                pass
            return why
        return ""

    def _collect_outputs(self, changed: list[Path], out_dir: Path) -> list[str]:
        rels = []
        for p in changed:
            try:
                rel = p.resolve().relative_to(self.root).as_posix()
            except ValueError:
                continue
            if rel in rels:
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
        # Incremental decoding: a UTF-8 character split between two reads stays intact.
        decoder = codecs.getincrementaldecoder("utf-8")("replace")
        tail = ""  # end of the previous stderr chunk, so a message cut in two is still recognised
        while True:
            chunk = await stream.read(4096)
            text = decoder.decode(chunk, final=not chunk)
            if any(c.isalnum() for c in text):  # a spinner alone ("-\\|/") isn't progress
                job._last_progress = time.time()
            if kind == "stderr" and not job._missing and not job._other_python and text:
                prog = programs.missing_in_output(tail + text)
                if prog and prog.pip and not job._pc_python:
                    job._other_python = prog.key
                elif prog:
                    job._missing = prog.key
                tail = (tail + text)[-512:]
            if text:
                job.emit({"type": kind, "data": text})
            if not chunk:
                break

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

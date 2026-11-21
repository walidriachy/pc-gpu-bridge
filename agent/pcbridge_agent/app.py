"""FastAPI application: the PC side of PC Bridge."""

from __future__ import annotations

import asyncio
import json
import math
import platform
import shutil
import socket
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from collections import OrderedDict
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import Depends, FastAPI, File, Form, Header, HTTPException, UploadFile
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from pydantic import BaseModel, Field

from . import __version__, programs
from .cache import ResultCache, cache_key
from .cleanup import cleanup_old
from .config import PathOutsideRoot, Settings, check_argv_paths, resolve_claude, safe_path
from .gpu import query_gpus
from .jobs import TERMINAL, Job, JobQueue, exit_event
from .memory import memory_info
from .pairing import PairingStore
from .syncthing import Syncthing, SyncthingError, folder_id

TOOLS = ("ffmpeg", "ffprobe", "python", "claude", "blender", "docker", "node", "npm", "pnpm", "ollama", "git")
TORCH_CHECK_S = 60.0  # how long /status reuses its "is PyTorch installed?" answer
COMFY_URL = "http://127.0.0.1:8188"
COMFY_POLL_S = 1.0
COMFY_STOP_WAIT_S = 30.0  # after a timeout or cancel, how long to wait for ComfyUI to let go of the GPU
# How long ComfyUI may keep failing to answer (busy loading a model, a bad reply) before we give up.
COMFY_UNANSWERED_S = 120.0


class InstallRequest(BaseModel):
    program: str
    stream: bool = True


class RunRequest(BaseModel):
    argv: list[str] = Field(min_length=1)
    cwd: str = "workspace"
    name: str | None = None
    gpu: bool = True
    inputs: list[str] = Field(default_factory=list)
    cache: bool = True
    env: dict[str, str] = Field(default_factory=dict)
    stream: bool = True


class DelegateRequest(BaseModel):
    task: str = Field(min_length=1)
    name: str | None = None
    files: list[str] = Field(default_factory=list)  # workspace-relative files copied into the worker dir
    stream: bool = True


class PairRequest(BaseModel):
    code: str
    device_name: str = "Mac"
    device_id: str | None = None


class ClaimRequest(BaseModel):
    device_id: str
    device_name: str = "Mac"


class ShareRequest(BaseModel):
    mac_device_id: str
    mac_name: str = "Mac"
    folders: list[str]  # folder names inside workspace/


class OllamaChat(BaseModel):
    model: str
    messages: list[dict[str, Any]]
    options: dict[str, Any] = Field(default_factory=dict)


class ComfyRequest(BaseModel):
    workflow: dict[str, Any]
    timeout: float = 600


def _mac_address() -> str:
    n = uuid.getnode()
    return ":".join(f"{(n >> s) & 0xFF:02x}" for s in range(40, -1, -8))


def _http_json(url: str, body: dict | None = None, timeout: float = 600) -> Any:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:  # noqa: S310 - localhost only
        raw = r.read().decode()
    return json.loads(raw) if raw.strip() else None  # ComfyUI answers some POSTs with no body


def _comfy_running(base: str, pid: str) -> bool:
    q = _http_json(f"{base}/queue", None, 10) or {}
    return any(len(item) > 1 and item[1] == pid for item in q.get("queue_running", []))


def _comfy_stop(base: str, pid: str, wait: float) -> None:
    """Best effort: drop our prompt from ComfyUI's queue, or interrupt it and wait until it stops.

    Blocking: run it in a worker thread. Only our prompt is touched, never someone else's.
    """
    try:
        if not _comfy_running(base, pid):
            _http_json(f"{base}/queue", {"delete": [pid]}, 10)
            return
        _http_json(f"{base}/interrupt", {"prompt_id": pid}, 10)
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline and _comfy_running(base, pid):
            time.sleep(COMFY_POLL_S)
    except (urllib.error.URLError, OSError, ValueError, TypeError, AttributeError):
        pass


def _comfy_error(entry: dict) -> str | None:
    """ComfyUI's own error message when a prompt failed, else None."""
    status = entry.get("status") or {}
    if status.get("status_str") != "error":
        return None
    for msg in status.get("messages") or []:
        if isinstance(msg, list) and len(msg) > 1 and msg[0] == "execution_error":
            data = msg[1] if isinstance(msg[1], dict) else {}
            node = data.get("node_type") or data.get("node_id") or "a node"
            return f"{node}: {data.get('exception_message', 'unknown error')}".strip()
    return "the workflow failed"


class WarmModels:
    """Keep up to N Ollama models loaded in VRAM; evict least-recently-used."""

    def __init__(self, limit: int = 2):
        self.limit = limit
        self.models: OrderedDict[str, float] = OrderedDict()

    def touch(self, model: str) -> list[str]:
        self.models.pop(model, None)
        self.models[model] = time.time()
        evicted = []
        while len(self.models) > self.limit:
            old, _ = self.models.popitem(last=False)
            evicted.append(old)
        return evicted


def create_app(settings: Settings | None = None) -> FastAPI:
    settings = settings or Settings.load()
    root = settings.root_path
    pairing = PairingStore(root / "config")
    pairing.ensure_code()
    pairing.local_token()
    queue = JobQueue(root, settings.max_parallel_jobs)
    queue.load_history()  # the Jobs list survives restarts
    cache = ResultCache(root / "cache", root, int(settings.cache_max_gb * 1024**3))
    warm = WarmModels()
    pending_cache: dict[str, str] = {}

    torch_seen: dict[str, float | bool] = {}

    def on_finish(job: Job, changed: list[Path]) -> None:
        if job.kind == "install":
            torch_seen.clear()  # /status checks PyTorch again
        key = pending_cache.pop(job.id, None)
        if key:
            cache.put(key, changed, job.exit_code or 0)

    queue.on_finish = on_finish

    @asynccontextmanager
    async def lifespan(_app: FastAPI):
        cleanup_old(root, settings.cleanup_days)

        async def daily():
            while True:
                await asyncio.sleep(24 * 3600)
                await asyncio.to_thread(cleanup_old, root, settings.cleanup_days)
                await asyncio.to_thread(cache.enforce_cap)

        task = asyncio.get_running_loop().create_task(daily())
        yield
        task.cancel()

    app = FastAPI(title="PC Bridge Agent", version=__version__, lifespan=lifespan)
    app.state.settings = settings
    app.state.queue = queue
    app.state.pairing = pairing
    app.state.cache = cache

    def auth(authorization: str | None = Header(default=None)) -> None:
        token = (authorization or "").removeprefix("Bearer ").strip()
        if not pairing.verify(token):
            raise HTTPException(401, "Not paired: missing or wrong token")

    @app.exception_handler(PathOutsideRoot)
    async def _outside(_, exc: PathOutsideRoot):
        return JSONResponse({"detail": str(exc)}, status_code=403)

    # ---- public ----------------------------------------------------------
    @app.get("/health")
    async def health():
        return {"ok": True, "service": "pcbridge-agent", "version": __version__}

    @app.post("/pair")
    async def pair(req: PairRequest):
        token = pairing.redeem(req.code, req.device_name, req.device_id)
        if not token:
            raise HTTPException(403, "That code is wrong or expired. Check the code on the PC screen.")
        return {
            "token": token,
            "hostname": socket.gethostname(),
            "root": str(root),
            "mac_address": settings.mac_address or _mac_address(),
            "platform": platform.platform(),
            "version": __version__,
        }

    # ---- authenticated ---------------------------------------------------
    def _token(authorization: str | None) -> str:
        return (authorization or "").removeprefix("Bearer ").strip()

    @app.post("/pair/claim", dependencies=[Depends(auth)])
    async def pair_claim(req: ClaimRequest, authorization: str | None = Header(default=None)):
        """A Mac tags its own entry with its device id; older duplicates of it are removed."""
        return {"removed": pairing.claim(_token(authorization), req.device_id, req.device_name)}

    @app.post("/pair/forget", dependencies=[Depends(auth)])
    async def pair_forget(authorization: str | None = Header(default=None)):
        """A Mac unpairing itself: its token stops working."""
        return {"forgotten": pairing.forget(_token(authorization))}

    @app.get("/pair/devices", dependencies=[Depends(auth)])
    async def pair_devices():
        return {"devices": pairing.devices()}

    @app.delete("/pair/devices/{index}", dependencies=[Depends(auth)])
    async def pair_remove(index: int, authorization: str | None = Header(default=None)):
        if not pairing.is_local(_token(authorization)):
            raise HTTPException(403, "Only PC Bridge on the PC itself can remove paired Macs.")
        if not pairing.remove(index):
            raise HTTPException(404, "No paired Mac with that number.")
        return {"devices": pairing.devices()}

    @app.post("/pair/new", dependencies=[Depends(auth)])
    async def pair_new():
        """Fresh pairing code (used by the PC's own app via its local token)."""
        return {"code": pairing.new_code(), "expires_in": 15 * 60}

    @app.get("/pair/code", dependencies=[Depends(auth)])
    async def pair_code():
        return {"code": pairing.ensure_code()}

    peer_memory: dict[str, Any] = {}

    def tools_present() -> dict[str, bool]:
        # Same lookups as /programs, so Blender (not on PATH) counts once it's installed.
        found = {t: bool(programs.locate(t)) for t in TOOLS}
        found["claude"] = bool(resolve_claude(settings.claude_cmd))
        if time.monotonic() - float(torch_seen.get("at", -math.inf)) > TORCH_CHECK_S:
            torch_seen["ok"] = programs.is_installed(programs.CATALOG["pytorch"])
            torch_seen["at"] = time.monotonic()
        found["pytorch"] = bool(torch_seen["ok"])
        return found

    @app.get("/status", dependencies=[Depends(auth)])
    async def status():
        running = [queue.jobs[j].summary() for j in queue.running if j in queue.jobs]
        # Disk and PATH lookups run in a worker thread so the agent keeps answering meanwhile.
        cache_bytes, mem, tools = await asyncio.to_thread(
            lambda: (cache.size(), memory_info(), tools_present())
        )
        return {
            "ok": True,
            "hostname": socket.gethostname(),
            "root": str(root),
            "version": __version__,
            "queue_depth": queue.queue_depth(),
            "waiting": len(queue.waiting),
            "running": running,
            "max_parallel_jobs": settings.max_parallel_jobs,
            "cache_bytes": cache_bytes,
            "memory": mem,
            "tools": tools,
            "devices": pairing.devices(),
            "peer_memory": peer_memory or None,
        }

    @app.post("/peer/memory", dependencies=[Depends(auth)])
    async def report_peer_memory(body: dict[str, Any]):
        """The Mac reports its own free RAM so the PC app can show Mac vs PC side by side."""
        keep = ("total_bytes", "available_bytes", "pressure", "free_percent")
        peer_memory.clear()
        peer_memory.update({k: body[k] for k in keep if k in body})
        peer_memory["ts"] = time.time()
        return {"ok": True}

    @app.get("/gpu", dependencies=[Depends(auth)])
    async def gpu():
        return await asyncio.to_thread(query_gpus, settings.nvidia_smi_cmd)

    @app.get("/memory", dependencies=[Depends(auth)])
    async def memory():
        return await asyncio.to_thread(memory_info)

    @app.get("/jobs", dependencies=[Depends(auth)])
    async def jobs(limit: int = 50):
        items = sorted(queue.jobs.values(), key=lambda j: j.created, reverse=True)[:limit]
        return [j.summary() for j in items]

    @app.get("/jobs/{jid}", dependencies=[Depends(auth)])
    async def job(jid: str):
        j = queue.jobs.get(jid)
        if not j:
            raise HTTPException(404, "No such job")
        return j.summary()

    @app.get("/jobs/{jid}/events", dependencies=[Depends(auth)])
    async def job_events(jid: str, start: int = 0):
        j = queue.jobs.get(jid)
        if not j:
            raise HTTPException(404, "No such job")
        return _stream(j, start)

    @app.post("/jobs/{jid}/cancel", dependencies=[Depends(auth)])
    async def cancel(jid: str):
        if not await queue.cancel(jid):
            raise HTTPException(404, "Job not found or already finished")
        return {"ok": True}

    def _stream(job: Job, start: int = 0) -> StreamingResponse:
        async def gen():
            async for ev in job.follow(start):
                yield json.dumps(ev) + "\n"

        return StreamingResponse(gen(), media_type="application/x-ndjson")

    async def _finish(job: Job, stream: bool):
        if stream:
            return _stream(job)
        async for _ in job.follow():
            pass
        return {**job.summary(), "log": "".join(e.get("data", "") for e in job.events if "data" in e)}

    @app.post("/run", dependencies=[Depends(auth)])
    async def run(req: RunRequest):
        prog = Path(req.argv[0].replace("\\", "/")).stem.lower()
        if settings.allowlist and prog not in {a.lower() for a in settings.allowlist}:
            raise HTTPException(403, f"'{prog}' is not in the PC's allowed programs list")
        check_argv_paths(root, req.argv)
        cwd = safe_path(root, req.cwd)
        cwd.mkdir(parents=True, exist_ok=True)
        inputs = [safe_path(root, p) for p in req.inputs]
        inputs = [p for p in inputs if p.is_file()]

        job = queue.create("run", req.name or prog, req.argv, cwd, gpu=req.gpu, env=req.env)
        if req.cache and inputs:
            key = await asyncio.to_thread(cache_key, req.argv, inputs)
            restored = await asyncio.to_thread(cache.get, key)
            if restored is not None:
                job.cached = True
                job.state = "done"
                job.exit_code = 0
                job.started = job.exited = job.finished = time.time()
                job.outputs = restored
                job.write_meta()
                job.emit({"type": "cached", "outputs": restored})
                job.emit(exit_event(job, 0))
                return await _finish(job, req.stream)
            pending_cache[job.id] = key
        await queue.submit(job)
        return await _finish(job, req.stream)

    # ---- programs on the PC ----------------------------------------------------
    @app.get("/programs", dependencies=[Depends(auth)])
    async def programs_list():
        def work():
            items = [
                {
                    "key": prog.key,
                    "name": prog.name,
                    "why": prog.why,
                    "size_gb": prog.size_gb,
                    "installed": programs.is_installed(prog),
                }
                for prog in programs.CATALOG.values()
            ]
            return {
                "programs": items,
                "free_gb": round(programs.free_gb(), 1),
                "drive": str(programs.system_drive()),
            }

        return await asyncio.to_thread(work)

    @app.post("/programs/install", dependencies=[Depends(auth)])
    async def programs_install(req: InstallRequest):
        prog = programs.lookup(req.program)
        if not prog:
            known = ", ".join(programs.CATALOG)
            raise HTTPException(
                404, f"PC Bridge can't install “{req.program}”. Programs it can install: {known}."
            )
        name = f"install {prog.name}"
        free = await asyncio.to_thread(programs.free_gb)
        needed = prog.size_gb + 2  # the download plus room to unpack it
        if 0 <= free < needed:
            drive = str(programs.system_drive()).rstrip("\\/")
            raise HTTPException(
                507, f"Not enough free space on the PC's {drive} drive for {prog.name}: "
                f"{free:.1f} GB free, about {math.ceil(needed)} GB needed. "
                "Free up some space on the PC, then try again."
            )  # fmt: skip
        try:
            argv = programs.install_argv(prog)
        except RuntimeError as exc:
            raise HTTPException(503, str(exc)) from exc
        # Clicked twice, or asked by both Claude and the PC app: follow the install already going.
        # No await between this check and queue.create, so two requests can't both pass it.
        for other in queue.jobs.values():
            if other.kind == "install" and other.name == name and other.state not in TERMINAL:
                return await _finish(other, req.stream)
        job = queue.create("install", name, argv, root / "worker", gpu=False)
        await queue.submit(job)
        return await _finish(job, req.stream)

    @app.post("/delegate", dependencies=[Depends(auth)])
    async def delegate(req: DelegateRequest):
        claude = resolve_claude(settings.claude_cmd) or settings.claude_cmd
        job = queue.create("delegate", req.name or req.task[:40], [], root / "worker", gpu=True)
        wdir = root / "worker" / job.id
        wdir.mkdir(parents=True, exist_ok=True)
        for rel in req.files:
            src = safe_path(root, rel)
            if src.is_file():
                shutil.copy2(src, wdir / src.name)
        job.cwd = wdir
        job.argv = [
            claude,
            "-p",
            req.task,
            "--output-format",
            "stream-json",
            "--verbose",
            *settings.delegate_args,
        ]
        await queue.submit(job)
        return await _finish(job, req.stream)

    @app.post("/files/push", dependencies=[Depends(auth)])
    async def files_push(path: str = Form(...), file: UploadFile = File(...)):
        dest = safe_path(root, path)
        dest.parent.mkdir(parents=True, exist_ok=True)
        with open(dest, "wb") as f:
            while chunk := await file.read(1 << 20):
                f.write(chunk)
        return {"ok": True, "path": dest.relative_to(root).as_posix(), "bytes": dest.stat().st_size}

    @app.get("/files/stat", dependencies=[Depends(auth)])
    async def files_stat(path: str):
        p = safe_path(root, path)
        if not p.is_file():
            return {"exists": False}
        st = p.stat()
        return {"exists": True, "size": st.st_size, "mtime": st.st_mtime}

    @app.get("/files/pull", dependencies=[Depends(auth)])
    async def files_pull(path: str):
        src = safe_path(root, path)
        if not src.is_file():
            raise HTTPException(404, "File not found on the PC")
        return FileResponse(src, filename=src.name)

    @app.post("/files/speedtest", dependencies=[Depends(auth)])
    async def speedtest(file: UploadFile = File(...)):
        start = time.perf_counter()
        n = 0
        while chunk := await file.read(1 << 20):
            n += len(chunk)
        return {"bytes": n, "server_seconds": time.perf_counter() - start}

    @app.get("/files/speedtest", dependencies=[Depends(auth)])
    async def speedtest_down(mb: int = 20):
        mb = max(1, min(mb, 200))

        async def gen():
            block = b"\0" * (1 << 20)
            for _ in range(mb):
                yield block

        return StreamingResponse(gen(), media_type="application/octet-stream")

    # ---- Syncthing pairing -------------------------------------------------
    def _syncthing() -> Syncthing:
        if not settings.syncthing_api_key:
            raise HTTPException(503, "Syncthing isn't set up on the PC yet (open PC Bridge on the PC)")
        return Syncthing(settings.syncthing_api_key, settings.syncthing_url)

    @app.get("/syncthing/info", dependencies=[Depends(auth)])
    async def syncthing_info():
        try:
            return {"device_id": await asyncio.to_thread(_syncthing().device_id)}
        except SyncthingError as exc:
            raise HTTPException(503, str(exc)) from exc

    @app.post("/syncthing/share", dependencies=[Depends(auth)])
    async def syncthing_share(req: ShareRequest):
        st = _syncthing()

        def work():
            pc_id = st.device_id()
            st.add_device(req.mac_device_id, req.mac_name)
            shared = []
            for name in req.folders:
                path = safe_path(root, f"workspace/{name}")
                st.share_folder(folder_id(name), name, path, [req.mac_device_id, pc_id])
                shared.append({"id": folder_id(name), "name": name, "path": str(path)})
            return {"pc_device_id": pc_id, "folders": shared}

        try:
            return await asyncio.to_thread(work)
        except SyncthingError as exc:
            raise HTTPException(503, str(exc)) from exc

    # ---- optional local AI tools ------------------------------------------
    # Ollama chats don't take the GPU slot: a reply would wait behind an hour-long render, and Ollama
    # fits (or partly offloads) models to the VRAM that is free by itself.
    @app.post("/ollama/chat", dependencies=[Depends(auth)])
    async def ollama_chat(req: OllamaChat):
        evicted = warm.touch(req.model)
        try:
            for old in evicted:
                await asyncio.to_thread(
                    _http_json, "http://127.0.0.1:11434/api/generate", {"model": old, "keep_alive": 0}, 30
                )
            return await asyncio.to_thread(
                _http_json,
                "http://127.0.0.1:11434/api/chat",
                {
                    "model": req.model,
                    "messages": req.messages,
                    "options": req.options,
                    "stream": False,
                    "keep_alive": -1,
                },
            )
        except (urllib.error.URLError, OSError) as exc:
            raise HTTPException(503, f"Ollama is not running on the PC: {exc}") from exc

    @app.post("/comfy/generate", dependencies=[Depends(auth)])
    async def comfy_generate(req: ComfyRequest):
        """Run a ComfyUI workflow while holding a GPU slot, like any other GPU job.

        The job runs in the background (it keeps its place if the client goes away), and this
        request waits for it. ``timeout`` counts from when the workflow reaches ComfyUI.
        """
        base = COMFY_URL
        try:  # before queueing, so nobody waits an hour to hear ComfyUI isn't there
            await asyncio.to_thread(_http_json, f"{base}/system_stats", None, 10)
        except (urllib.error.URLError, OSError, ValueError) as exc:
            raise HTTPException(503, f"ComfyUI is not running on the PC: {exc}") from exc
        job = queue.create("comfy", "comfy-generate", ["comfyui"], root / "workspace", gpu=True)
        problem: list[tuple[int, str]] = []

        def fail(status: int, detail: str) -> int:
            problem.append((status, detail))
            job.emit({"type": "stderr", "data": f"PC Bridge: {detail}\n"})
            return 1

        async def work(job: Job) -> int:
            try:
                sub = await asyncio.to_thread(_http_json, f"{base}/prompt", {"prompt": req.workflow}, 30)
            except urllib.error.HTTPError as exc:
                body = exc.read().decode("utf-8", "replace")[:500]
                return fail(503, f"ComfyUI refused the workflow (HTTP {exc.code}): {body}")
            except (urllib.error.URLError, OSError, ValueError) as exc:
                return fail(503, f"ComfyUI is not running on the PC: {exc}")
            pid = (sub or {}).get("prompt_id")
            if not pid:
                return fail(503, f"ComfyUI did not accept the workflow: {sub}")
            job.emit({"type": "stdout", "data": f"ComfyUI prompt {pid} queued\n"})
            deadline = time.monotonic() + req.timeout
            unanswered_since: float | None = None
            while not job._cancel:
                if time.monotonic() >= deadline:
                    await asyncio.to_thread(_comfy_stop, base, pid, COMFY_STOP_WAIT_S)
                    return fail(504, "ComfyUI did not finish in time")
                try:
                    hist = await asyncio.to_thread(_http_json, f"{base}/history/{pid}", None, 30) or {}
                except (urllib.error.URLError, OSError, ValueError) as exc:
                    # The prompt may still be running: one missed answer must not free the GPU slot.
                    now = time.monotonic()
                    unanswered_since = unanswered_since or now
                    if now - unanswered_since < COMFY_UNANSWERED_S:
                        await asyncio.sleep(COMFY_POLL_S)
                        continue
                    await asyncio.to_thread(_comfy_stop, base, pid, COMFY_STOP_WAIT_S)
                    return fail(503, f"ComfyUI stopped answering: {exc}")
                unanswered_since = None
                if pid in hist:
                    job.exited = time.time()  # saving the images below is output collection
                    error = _comfy_error(hist[pid])
                    if error:
                        job.emit({"type": "stderr", "data": f"ComfyUI: {error}\n"})
                        return 1
                    for node in hist[pid].get("outputs", {}).values():
                        for img in node.get("images", []):
                            q = urllib.parse.urlencode(img)
                            dest = job.folder / "output" / Path(img["filename"]).name
                            await asyncio.to_thread(urllib.request.urlretrieve, f"{base}/view?{q}", dest)
                            job.outputs.append(dest.relative_to(root).as_posix())
                    return 0
                await asyncio.sleep(COMFY_POLL_S)
            await asyncio.to_thread(_comfy_stop, base, pid, COMFY_STOP_WAIT_S)
            return -1

        await queue.submit(job, work)
        async for _ in job.follow():
            pass
        if problem:
            raise HTTPException(*problem[0])
        return job.summary()

    return app

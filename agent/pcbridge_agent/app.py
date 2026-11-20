"""FastAPI application: the PC side of PC Bridge."""

from __future__ import annotations

import asyncio
import json
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

from . import __version__
from .cache import ResultCache, cache_key
from .cleanup import cleanup_old
from .config import PathOutsideRoot, Settings, check_argv_paths, safe_path
from .gpu import query_gpus
from .jobs import Job, JobQueue
from .memory import memory_info
from .pairing import PairingStore

TOOLS = ("ffmpeg", "ffprobe", "python", "claude", "blender", "docker", "node", "npm", "pnpm", "ollama")


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
        return json.loads(r.read().decode())


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
    cache = ResultCache(root / "cache", root, int(settings.cache_max_gb * 1024**3))
    warm = WarmModels()
    pending_cache: dict[str, str] = {}

    def on_finish(job: Job, changed: list[Path]) -> None:
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
                cleanup_old(root, settings.cleanup_days)
                cache.enforce_cap()

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
        token = pairing.redeem(req.code, req.device_name)
        if not token:
            raise HTTPException(403, "That code is wrong or expired. Check the code on the PC screen.")
        return {
            "token": token,
            "hostname": socket.gethostname(),
            "root": str(root),
            "mac_address": _mac_address(),
            "platform": platform.platform(),
            "version": __version__,
        }

    # ---- authenticated ---------------------------------------------------
    @app.get("/status", dependencies=[Depends(auth)])
    async def status():
        running = [queue.jobs[j].summary() for j in queue.running if j in queue.jobs]
        return {
            "ok": True,
            "hostname": socket.gethostname(),
            "root": str(root),
            "version": __version__,
            "queue_depth": queue.queue_depth(),
            "waiting": len(queue.waiting),
            "running": running,
            "max_parallel_jobs": settings.max_parallel_jobs,
            "cache_bytes": cache.size(),
            "memory": await asyncio.to_thread(memory_info),
            "tools": {t: bool(shutil.which(t)) for t in TOOLS},
            "devices": pairing.devices(),
        }

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
                job.started = job.finished = time.time()
                job.outputs = restored
                job.write_meta()
                job.emit({"type": "cached", "outputs": restored})
                job.emit({"type": "exit", "code": 0, "state": "done", "duration": 0, "outputs": restored})
                return await _finish(job, req.stream)
            pending_cache[job.id] = key
        await queue.submit(job)
        return await _finish(job, req.stream)

    @app.post("/delegate", dependencies=[Depends(auth)])
    async def delegate(req: DelegateRequest):
        claude = shutil.which(settings.claude_cmd) or settings.claude_cmd
        job = queue.create("delegate", req.name or req.task[:40], [], root / "worker", gpu=True)
        wdir = root / "worker" / job.id
        wdir.mkdir(parents=True, exist_ok=True)
        for rel in req.files:
            src = safe_path(root, rel)
            if src.is_file():
                shutil.copy2(src, wdir / src.name)
        job.cwd = wdir
        job.argv = [claude, "-p", req.task, "--output-format", "stream-json", "--verbose",
                    *settings.delegate_args]
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

    # ---- optional local AI tools ------------------------------------------
    @app.post("/ollama/chat", dependencies=[Depends(auth)])
    async def ollama_chat(req: OllamaChat):
        evicted = warm.touch(req.model)
        try:
            for old in evicted:
                await asyncio.to_thread(_http_json, "http://127.0.0.1:11434/api/generate",
                                        {"model": old, "keep_alive": 0}, 30)
            return await asyncio.to_thread(
                _http_json, "http://127.0.0.1:11434/api/chat",
                {"model": req.model, "messages": req.messages, "options": req.options,
                 "stream": False, "keep_alive": -1},
            )
        except (urllib.error.URLError, OSError) as exc:
            raise HTTPException(503, f"Ollama is not running on the PC: {exc}") from exc

    @app.post("/comfy/generate", dependencies=[Depends(auth)])
    async def comfy_generate(req: ComfyRequest):
        base = "http://127.0.0.1:8188"
        try:
            sub = await asyncio.to_thread(_http_json, f"{base}/prompt", {"prompt": req.workflow}, 30)
        except (urllib.error.URLError, OSError) as exc:
            raise HTTPException(503, f"ComfyUI is not running on the PC: {exc}") from exc
        pid = sub.get("prompt_id")
        job = queue.create("comfy", "comfy-generate", ["comfyui"], root / "workspace", gpu=True)
        deadline = time.time() + req.timeout
        while time.time() < deadline:
            hist = await asyncio.to_thread(_http_json, f"{base}/history/{pid}", None, 30)
            if pid in hist:
                saved = []
                for node in hist[pid].get("outputs", {}).values():
                    for img in node.get("images", []):
                        q = urllib.parse.urlencode(img)
                        dest = job.folder / "output" / img["filename"]
                        await asyncio.to_thread(urllib.request.urlretrieve, f"{base}/view?{q}", dest)
                        saved.append(dest.relative_to(root).as_posix())
                job.outputs, job.state, job.exit_code = saved, "done", 0
                job.finished = time.time()
                job.write_meta()
                job.emit({"type": "exit", "code": 0, "state": "done", "outputs": saved})
                return job.summary()
            await asyncio.sleep(1)
        job.state = "failed"
        job.emit({"type": "exit", "code": 1, "state": "failed"})
        raise HTTPException(504, "ComfyUI did not finish in time")

    return app

